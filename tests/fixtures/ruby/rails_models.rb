class User < ApplicationRecord
  has_many :posts, dependent: :destroy
  has_many :comments, through: :posts
  has_one :profile
  belongs_to :organization, class_name: 'Company'

  validates :name, presence: true, uniqueness: true
  validates :email, format: { with: URI::MailTo::EMAIL_REGEXP }
end

class Post < ApplicationRecord
  belongs_to :user
  has_many :comments
  has_and_belongs_to_many :tags

  validates :title, presence: true
  validates :body, length: { minimum: 10 }
end

class UsersController < ApplicationController
  before_action :authenticate_user, only: [:edit, :update]
  before_action :set_user, only: [:show, :edit, :update, :destroy]
  after_action :log_access

  def index
    @users = User.all
  end

  def show
  end

  private

  def set_user
    @user = User.find(params[:id])
  end

  def authenticate_user
    redirect_to login_path unless logged_in?
  end

  def log_access
    Rails.logger.info("Accessed users")
  end
end
